use super::*;

impl Provider {
    pub(super) fn state(&self, value: &Value, resource: &str, expected: &Amount) -> Result<State> {
        let status = value["status"].as_str().ok_or(Error::Rejected)?;
        let refund = resource == "refunds";
        let state = match self {
            Self::Mollie { .. } => match (refund, status) {
                (false, "paid") => State::Collected,
                (false, "authorized") => State::Authorized,
                (true, "refunded") => State::Refunded,
                (_, "open") => State::RequiresAction,
                (_, "pending" | "queued" | "processing") => State::Pending,
                (false, "canceled") => State::Cancelled,
                (_, "failed" | "expired" | "canceled") => State::Failed,
                _ => State::Unknown,
            },
            Self::Square { .. } => match (refund, status) {
                (false, "COMPLETED") => State::Collected,
                (false, "APPROVED") => State::Authorized,
                (true, "COMPLETED") => State::Refunded,
                (false, "CANCELED") => State::Cancelled,
                (_, "PENDING") => State::Pending,
                (_, "FAILED" | "REJECTED") => State::Failed,
                _ => State::Unknown,
            },
            Self::GoCardless { .. } => match (refund, status) {
                (false, "confirmed") => State::Collected,
                (false, "paid_out") => State::Collected,
                (true, "paid") => State::Refunded,
                (false, "cancelled") => State::Cancelled,
                (_, "pending_customer_approval") => State::RequiresAction,
                (_, "pending_submission" | "submitted" | "pending" | "created") => State::Pending,
                (_, "failed" | "customer_approval_denied" | "bounced" | "funds_returned") => {
                    State::Failed
                }
                _ => State::Unknown,
            },
            Self::Btcpay { .. } => match status {
                "New" => State::RequiresAction,
                "Processing" => State::Pending,
                "Settled"
                    if value["additionalStatus"] == "None"
                        || value["additionalStatus"] == "PaidOver" =>
                {
                    State::Settled
                }
                "Expired" | "Invalid" => State::Unknown,
                _ => State::Unknown,
            },
            Self::CoinGate { .. } => match status {
                "new" | "pending" => State::RequiresAction,
                "confirming" => State::Pending,
                "paid" => State::Settled,
                "refunded" => State::Refunded,
                "expired" | "invalid" | "canceled" => State::Failed,
                _ => State::Unknown,
            },
        };
        if matches!(
            state,
            State::Authorized | State::Collected | State::Settled | State::Refunded
        ) {
            let actual = self.amount(value)?;
            if !expected.matches(actual) {
                return Err(Error::Rejected);
            }
            if matches!(self, Self::Square { .. })
                && !refund
                && !matches!(
                    value["source_type"].as_str(),
                    Some(
                        "CARD" | "BANK_ACCOUNT" | "WALLET" | "BUY_NOW_PAY_LATER" | "SQUARE_ACCOUNT"
                    )
                )
            {
                return Err(Error::Rejected);
            }
            if matches!(self, Self::Btcpay { .. }) {
                let paid = value["paidAmount"]
                    .as_str()
                    .and_then(|s| Decimal::from_str_exact(s).ok())
                    .ok_or(Error::Rejected)?;
                if paid < Decimal::from_str_exact(&expected.value).map_err(|_| Error::Rejected)? {
                    return Err(Error::Rejected);
                }
            }
            // Mollie can mark an order paid after partial manual capture. Preserve
            // uncertainty when captured money does not cover the bound amount.
            if matches!(self, Self::Mollie { .. }) && state == State::Collected {
                if let Some(captured) = value.get("amountCaptured") {
                    if !expected.matches(Amount::from(captured, "value", "currency")?) {
                        return Err(Error::Rejected);
                    }
                } else if value["captureMode"] == "manual" {
                    return Err(Error::Rejected);
                }
            }
        }
        Ok(state)
    }
    pub(super) fn authorization_expiry(&self, value: &Value) -> Result<Option<u64>> {
        let expiry = match self {
            Self::Mollie { .. } => value.get("captureBefore"),
            _ => value.get("delayed_until"),
        };
        expiry
            .map(|value| {
                let text = value.as_str().ok_or(Error::Rejected)?;
                let date = time::OffsetDateTime::parse(
                    text,
                    &time::format_description::well_known::Rfc3339,
                )
                .map_err(|_| Error::Rejected)?;
                u64::try_from(date.unix_timestamp()).map_err(|_| Error::Rejected)
            })
            .transpose()
    }
    pub(super) fn action<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        match self {
            Self::Mollie { .. } => value.pointer("/_links/checkout/href"),
            Self::Btcpay { .. } => value.get("checkoutLink"),
            Self::CoinGate { .. } => value.get("payment_url"),
            _ => None,
        }
        .filter(|v| v.is_string())
    }
}

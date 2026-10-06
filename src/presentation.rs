//! Format billing snapshots for the terminal; accounting retains original numeric amounts.
use crate::cost::{CostSnapshot, CostStatus};
use crate::dest::PaymentInfo;

impl CostStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting...",
            Self::Connected => "Connected",
            Self::Reconnected => "Reconnected",
            Self::Reconnecting => "Reconnecting...",
            Self::Unavailable => "Unavailable",
            Self::NotAvailable => "Not available",
        }
    }

    pub fn is_warning(self) -> bool {
        matches!(self, Self::Reconnecting | Self::Unavailable)
    }
}

/// Separate primary amounts from ancillary counts and connection state.
#[derive(Clone, Debug, Default)]
pub struct CostDisplay {
    pub amount: Option<String>,
    pub requests: Option<u64>,
    pub estimated: bool,
    pub status: CostStatus,
}

impl CostDisplay {
    pub fn from_snapshot(
        snapshot: &CostSnapshot,
        payment: Option<&PaymentInfo>,
        conversion_currency: Option<&str>,
    ) -> Self {
        let amount = snapshot.amounts.as_ref().map(|amounts| {
            if amounts.is_empty() {
                return "0.00000000".into();
            }
            amounts
                .iter()
                .map(|money| {
                    let prefix = if money.currency == "USD" { "$" } else { "" };
                    let converted = payment
                        .filter(|_| conversion_currency == Some(money.currency.as_str()))
                        .map(|payment| {
                            format!(
                                " ({:.6} {})",
                                money.amount * payment.exchange_rate,
                                payment.payment_currency
                            )
                        })
                        .unwrap_or_default();
                    format!("{prefix}{:.8} {}{converted}", money.amount, money.currency)
                })
                .collect::<Vec<_>>()
                .join(" + ")
        });
        Self {
            amount,
            requests: snapshot.requests,
            estimated: snapshot.estimated,
            status: snapshot.status,
        }
    }
}

impl std::fmt::Display for CostDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(amount) = &self.amount {
            write!(f, "{amount}")?;
            if let Some(requests) = self.requests {
                write!(f, " | {requests} Requests")?;
            }
            if self.estimated {
                write!(f, " · Estimated")?;
            }
            write!(f, " · ")?;
        }
        f.write_str(self.status.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::Money;

    #[test]
    fn original_currencies_remain_visible_and_conversion_applies_only_to_its_billing_currency() {
        let snapshot = CostSnapshot {
            amounts: Some(vec![
                Money {
                    amount: 2.0,
                    currency: "EUR".into(),
                },
                Money {
                    amount: 3.5,
                    currency: "USD".into(),
                },
            ]),
            requests: Some(2),
            estimated: true,
            status: CostStatus::Reconnecting,
        };
        let payment = PaymentInfo {
            payment_currency: "CNY".into(),
            exchange_rate: 7.0,
        };
        let original = "2.00000000 EUR + $3.50000000 USD";
        assert_eq!(
            CostDisplay::from_snapshot(&snapshot, None, Some("USD"))
                .amount
                .as_deref(),
            Some(original)
        );
        assert_eq!(
            CostDisplay::from_snapshot(&snapshot, Some(&payment), None)
                .amount
                .as_deref(),
            Some(original)
        );
        let display = CostDisplay::from_snapshot(&snapshot, Some(&payment), Some("USD"));
        assert_eq!(
            display.to_string(),
            "2.00000000 EUR + $3.50000000 USD (24.500000 CNY) | 2 Requests · Estimated · Reconnecting..."
        );
        // Missing billing is distinct from an estimated zero before the currency is known.
        assert!(
            CostDisplay::from_snapshot(&CostSnapshot::default(), None, None)
                .amount
                .is_none()
        );
        let zero = CostSnapshot {
            amounts: Some(Vec::new()),
            requests: Some(0),
            estimated: true,
            ..Default::default()
        };
        assert_eq!(
            CostDisplay::from_snapshot(&zero, None, None)
                .amount
                .as_deref(),
            Some("0.00000000")
        );
    }
}
